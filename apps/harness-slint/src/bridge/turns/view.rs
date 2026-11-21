use super::{
    background_row, diff_counts, measure_memory, now_s, peek_parts, plan_rows, subagent_row, sv,
    thought_preview, tokens, tool_parts, trails, BackgroundTask, ChatItem, Command, ContextUsage,
    ItemId, Model, ModelRc, PlanEntry, SharedString, SubagentRow, SubagentStatus, SubagentView,
    ToolContent, ToolLocation, UiState, UsagePart, VecModel, AUTO_COMPACT_AT,
    THOUGHT_PREVIEW_CHARS,
};

impl UiState {
    // ---- thinking rows (R8) ------------------------------------------------------------------------

    pub(in crate::bridge) fn thought(&mut self, text: &str) {
        if let Some((_, index)) = self.turn.thought {
            if let Some(mut row) = self.chat.row_data(index) {
                let mut full = row.text.to_string();
                full.push_str(text);
                if row.title.chars().count() < THOUGHT_PREVIEW_CHARS {
                    row.title = sv(&thought_preview(&full));
                }
                row.text = sv(&full);
                self.chat.set_row_data(index, row);
                return;
            }
        }
        let id = self.turn.next_thought;
        self.turn.next_thought -= 1;
        let index = self.chat.row_count();
        self.chat.push(ChatItem {
            id,
            kind: 5,
            agent: self.agent_name(&self.sticky).into(),
            text: sv(text),
            title: sv(&thought_preview(text)),
            status: 0,
            ..ChatItem::default()
        });
        self.turn.thought = Some((id, index));
        self.turn.thought_rows.insert(id, index);
        self.mark_continued(index);
        self.regroup(index);
    }

    pub(super) fn finish_thought(&mut self) {
        if let Some((_, index)) = self.turn.thought.take() {
            if let Some(mut row) = self.chat.row_data(index) {
                row.status = 1;
                self.chat.set_row_data(index, row);
            }
        }
    }

    pub(super) fn row_index(&self, id: i32) -> Option<usize> {
        if id < 0 {
            self.turn.thought_rows.get(&id).copied()
        } else {
            u64::try_from(id)
                .ok()
                .and_then(|id| self.rows.get(&id).copied())
        }
    }

    pub(super) fn toggle_run(&self, id: i32) {
        if let Some(index) = self.row_index(id) {
            self.toggle_group(index);
        }
    }

    pub(super) fn toggle_detail(&self, id: i32) {
        let Some(index) = self.row_index(id) else {
            return;
        };
        if let Some(mut row) = self.chat.row_data(index) {
            row.expanded = !row.expanded;
            self.chat.set_row_data(index, row);
        }
    }

    // ---- tool detail, plan, usage --------------------------------------------------------------------

    pub(in crate::bridge) fn tool_detail(
        &self,
        id: ItemId,
        kind: Option<&str>,
        content: &[ToolContent],
        locations: &[ToolLocation],
        raw_input: Option<&str>,
        raw_output: Option<&str>,
    ) {
        let parts = tool_parts(content, locations, raw_input, raw_output);
        let peek = peek_parts(&parts);
        let file = crate::live::row_file(kind, content, locations);
        let (added, removed) = diff_counts(content);
        self.update_row(id, |row| {
            row.added = added;
            row.removed = removed;
            row.tool_kind = sv(kind.unwrap_or_default());
            row.parts = ModelRc::new(VecModel::from(parts));
            row.peek = ModelRc::new(VecModel::from(peek));
            row.file = sv(&file.path);
            row.file_line = file.line;
            row.range = sv(&file.range);
        });
    }

    pub(in crate::bridge) fn plan(&self, entries: &[PlanEntry]) {
        self.ui()
            .set_plan(ModelRc::new(VecModel::from(plan_rows(entries))));
    }

    /// Subagent panel rows (§9.8); the elapsed clock restarts from now.
    pub(in crate::bridge) fn session_meta(
        &self,
        worktree: Option<&harness_core_api::WorktreeInfo>,
        cost: Option<&harness_core_api::SessionCost>,
    ) {
        let ui = self.ui();
        ui.set_worktree_branch(sv(worktree.map_or("", |w| w.branch.as_str())));
        ui.set_worktree_path(sv(worktree.map_or("", |w| w.path.as_str())));
        let money = |amount: &str| {
            amount
                .trim()
                .parse::<f64>()
                .map_or_else(|_| amount.to_owned(), |v| format!("${v:.2}"))
        };
        let text = cost.map_or_else(String::new, |c| match &c.subagents {
            Some(sub) => format!(
                "Session cost {}  ·  incl. subagents {}",
                money(&c.total.amount),
                money(&sub.amount)
            ),
            None => format!("Session cost {}", money(&c.total.amount)),
        });
        ui.set_session_cost(sv(&text));
    }

    pub(in crate::bridge) fn subagents(&self, items: &[SubagentView]) {
        let ui = self.ui();
        ui.set_now_s(now_s());
        let running = items.iter().filter(|s| s.status.is_active()).count();
        ui.set_subagents_running(i32::try_from(running).unwrap_or(i32::MAX));
        let waiting = items
            .iter()
            .filter(|s| matches!(s.status, SubagentStatus::NeedsApproval))
            .count();
        ui.set_subagents_waiting(i32::try_from(waiting).unwrap_or(i32::MAX));
        let rows: Vec<SubagentRow> = items
            .iter()
            .zip(trails(items))
            .map(|(s, (trail, steps))| SubagentRow {
                agent_name: sv(&self.agent_name(s.agent.as_str())),
                trail: ModelRc::new(VecModel::from(
                    trail.iter().map(|t| sv(t)).collect::<Vec<SharedString>>(),
                )),
                steps: i32::try_from(steps).unwrap_or(i32::MAX),
                ..subagent_row(s)
            })
            .collect();
        let active: Vec<SubagentRow> = items
            .iter()
            .zip(&rows)
            .filter(|(s, _)| s.status.is_active())
            .map(|(_, r)| r.clone())
            .collect();
        ui.set_subagents_active(ModelRc::new(VecModel::from(active)));
        ui.set_subagents(ModelRc::new(VecModel::from(rows)));
    }

    pub(in crate::bridge) fn background_tasks(&self, tasks: &[BackgroundTask]) {
        let ui = self.ui();
        ui.set_now_s(now_s());
        let running = tasks.iter().filter(|t| t.is_running()).count();
        ui.set_background_tasks_running(i32::try_from(running).unwrap_or(i32::MAX));
        ui.set_background_tasks(ModelRc::new(VecModel::from(
            tasks.iter().map(background_row).collect::<Vec<_>>(),
        )));
    }

    pub(super) fn stop_background_task(&self, id: &str) {
        if let Some(session) = self.active.clone() {
            self.send(Command::StopBackgroundTask {
                session,
                id: id.to_owned(),
            });
        }
    }

    pub(in crate::bridge) fn parent_link(&self, parent: Option<(String, String)>) {
        let ui = self.ui();
        let (id, title) = parent.unwrap_or_default();
        ui.set_parent_id(sv(&id));
        ui.set_parent_title(sv(&title));
    }

    pub(super) fn subagent_action(&self, action: &str, id: &str) {
        match action {
            "open" | "back" if !id.is_empty() => self.send(Command::OpenSession {
                session: id.to_owned(),
            }),
            "tick" => self.ui().set_now_s(now_s()),
            // Orchestra live view: stop the subagent's running turn (same as Stop in its own session).
            "stop" if !id.is_empty() => self.send(Command::Cancel {
                session: id.to_owned(),
            }),
            _ => {}
        }
    }

    pub(super) fn auto_compact(&mut self) {
        if !self.turn.compact_due || !self.ui().get_auto_compact() {
            return;
        }
        let Some(session) = self.active.clone() else {
            return;
        };
        self.turn.compact_due = false;
        self.turn.compacted = true;
        self.send_message(session, "/compact".into(), Vec::new());
    }

    pub(in crate::bridge) fn usage(&mut self, usage: &ContextUsage) {
        let ui = self.ui();
        ui.set_usage_known(usage.size > 0);
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let ratio = if usage.size == 0 {
            0.0
        } else {
            (usage.used as f64 / usage.size as f64).clamp(0.0, 1.0) as f32
        };
        ui.set_usage_ratio(ratio);
        let auto = ui.get_auto_compact();
        let buffer = usage
            .size
            .saturating_sub(usage.size.saturating_mul(85) / 100);
        ui.set_usage_used_text(sv(&tokens(usage.used)));
        ui.set_usage_buffer_text(sv(&tokens(buffer)));
        ui.set_usage_free_text(sv(&tokens(usage.size.saturating_sub(usage.used))));
        ui.set_usage_free_auto_text(sv(&tokens(
            usage.size.saturating_sub(usage.used).saturating_sub(buffer),
        )));
        ui.set_usage_text(sv(&format!(
            "{} / {}",
            tokens(usage.used),
            tokens(usage.size)
        )));
        ui.set_usage_cost(sv(&usage
            .cost
            .as_ref()
            .map(|c| format!("{} {}", c.amount, c.currency))
            .unwrap_or_default()));
        if ratio < AUTO_COMPACT_AT {
            self.turn.compact_due = false;
            self.turn.compacted = false;
        } else if auto && !self.turn.compacted {
            self.turn.compact_due = true;
        }
        self.turn.last_usage = (usage.size > 0).then_some((usage.used, usage.size));
        if !self.turn.memory_asked {
            self.turn.memory_asked = true;
            measure_memory(self.workspace.clone());
        }
        self.render_parts();
    }

    pub(in crate::bridge) fn render_parts(&self) {
        let Some((used, size)) = self.turn.last_usage else {
            return;
        };
        let ui = self.ui();
        let auto = ui.get_auto_compact();
        let skills = u64::try_from(crate::chat::skill_count()).unwrap_or(0);
        let estimates = [
            ("Skills", 1, skills.saturating_mul(45)),
            ("System tools", 2, 14_100),
            ("Memory files", 3, self.turn.memory_tokens.unwrap_or(0)),
            ("System prompt", 4, 2_900),
            ("MCP tools", 5, 2_000),
        ];
        let overhead: u64 = estimates.iter().map(|e| e.2).sum();
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let scale = |n: u64| -> u64 {
            if overhead <= used || overhead == 0 {
                n
            } else {
                (n as f64 * used as f64 / overhead as f64) as u64
            }
        };
        let fixed: Vec<(&str, i32, u64)> =
            estimates.iter().map(|e| (e.0, e.1, scale(e.2))).collect();
        let messages = used.saturating_sub(fixed.iter().map(|e| e.2).sum());
        let buffer = if auto {
            size.saturating_sub(size.saturating_mul(85) / 100)
        } else {
            0
        };
        let free = size.saturating_sub(used).saturating_sub(buffer);
        let mut parts = vec![("Messages", 0, messages)];
        parts.extend(fixed);
        if auto {
            parts.push((
                "Autocompact buffer",
                6,
                buffer.min(size.saturating_sub(used)),
            ));
        }
        parts.push(("Free space", 7, free));
        let mut start = 0.0_f32;
        let rows: Vec<UsagePart> = parts
            .into_iter()
            .filter(|p| p.2 > 0 || p.1 == 7)
            .map(|(label, tone, n)| {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let share = (n as f64 / size.max(1) as f64) as f32;
                let row = UsagePart {
                    label: sv(label),
                    tone,
                    amount: sv(&tokens(n)),
                    share,
                    start,
                };
                start += share;
                row
            })
            .collect();
        ui.set_usage_parts(ModelRc::new(VecModel::from(rows)));
    }
}
