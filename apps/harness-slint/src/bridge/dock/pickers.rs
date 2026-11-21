use super::{
    agy_agent, chosen_name, code_mode_item, matching_models, mode_name, option_rows, sv,
    toggle_session, ChatCall, Command, ContextItem, DockUi, MentionKind, MentionQuery, ModelRc,
    Options, SessionId, SharedString, UiState,
};

impl UiState {
    // ---- pickers (R13) -------------------------------------------------------------------------------

    /// `Event::SessionOptions` of the open session (`None` clears the pickers).
    pub(crate) fn apply_options(&mut self, options: Option<Options>) {
        let dock = self.dock();
        let Some(o) = options else {
            for set in [
                DockUi::set_modes,
                DockUi::set_models,
                DockUi::set_models_shown,
                DockUi::set_efforts,
            ] {
                set(&dock, ModelRc::default());
            }
            dock.set_model_query(SharedString::new());
            dock.set_picker_open(SharedString::new());
            dock.set_machine(false);
            crate::machine_view::set_session(&self.window, None, false, "", "");
            dock.set_research(self.active.is_none() && self.dock_state.draft_research);
            dock.set_code_mode(self.active.is_none() && self.dock_state.draft_code_mode);
            dock.set_code_mode_supported(!agy_agent(&self.sticky));
            self.dock_state.effort_id = None;
            self.dock_state.model_id = None;
            return;
        };
        dock.set_modes(option_rows(&o.modes));
        dock.set_models(option_rows(&o.models));
        // The menu's list is the full list narrowed by what is typed in its search box.
        let query = dock.get_model_query().to_string();
        dock.set_models_shown(option_rows(&matching_models(&o.models, &query)));
        // 修复：agy 的模型列表本身已带 Low/Medium/High 档位，再显示单独的 Effort 选项会重复，所以对 agy 隐藏。
        let agy = agy_agent(&self.sticky);
        dock.set_efforts(if agy {
            ModelRc::default()
        } else {
            option_rows(&o.efforts)
        });
        dock.set_mode(sv(o.mode.as_deref().unwrap_or_default()));
        dock.set_model(sv(o.model.as_deref().unwrap_or_default()));
        dock.set_effort(sv(o.effort.as_deref().unwrap_or_default()));
        dock.set_machine(o.machine);
        crate::machine_view::set_session(
            &self.window,
            self.active.as_deref(),
            o.machine,
            &self.sticky,
            &self.agent_name(&self.sticky),
        );
        let mode_label = if o.machine {
            "Machine".to_owned()
        } else {
            chosen_name(&o.modes, o.mode.as_deref())
        };
        dock.set_model_name(sv(&chosen_name(&o.models, o.model.as_deref())));
        dock.set_effort_name(if agy {
            SharedString::new()
        } else {
            sv(&chosen_name(&o.efforts, o.effort.as_deref()))
        });
        dock.set_options_locked(o.locked);
        dock.set_options_pending(o.pending);
        if o.locked {
            dock.set_picker_open(SharedString::new());
        }
        self.dock_state.effort_id = o.effort_id;
        self.dock_state.model_id = o.model_id;
        self.dock_state.mode_label = mode_label;
        self.show_modes();
    }

    pub(crate) fn show_modes(&self) {
        let (research, code) = self.active.as_ref().map_or(
            (
                self.dock_state.draft_research,
                self.dock_state.draft_code_mode,
            ),
            |session| {
                (
                    self.dock_state.research.contains(session),
                    self.dock_state.code_mode.contains(session),
                )
            },
        );
        let dock = self.dock();
        dock.set_research(research);
        dock.set_code_mode(code);
        dock.set_code_mode_supported(!agy_agent(&self.sticky));
        dock.set_mode_name(sv(&mode_name(&self.dock_state.mode_label, research, code)));
    }

    pub(crate) fn add_research(&mut self, session: &SessionId, context: &mut Vec<ContextItem>) {
        if context.contains(&ContextItem::Research) {
            self.dock_state.research.insert(session.clone());
            self.show_modes();
        } else if self.dock_state.research.contains(session) {
            context.push(ContextItem::Research);
        }
    }

    pub(crate) fn take_draft_research(&mut self, context: &mut Vec<ContextItem>) {
        if std::mem::take(&mut self.dock_state.draft_research) {
            context.push(ContextItem::Research);
        }
    }

    pub(crate) fn add_code_mode(&mut self, session: &SessionId, context: &mut Vec<ContextItem>) {
        if context.contains(&ContextItem::CodeMode) {
            self.dock_state.code_mode.insert(session.clone());
            self.show_modes();
        } else if let Some(item) = code_mode_item(
            self.dock_state.code_mode.contains(session),
            agy_agent(&self.sticky),
        ) {
            context.push(item);
        }
    }

    pub(crate) fn take_draft_code_mode(&mut self, context: &mut Vec<ContextItem>) {
        if let Some(item) = code_mode_item(self.dock_state.draft_code_mode, agy_agent(&self.sticky))
        {
            self.dock_state.draft_code_mode = false;
            context.push(item);
        }
    }

    pub(super) fn pick_option(&mut self, which: &str, id: &str) {
        if which == "mode" && id == "harness:research" {
            match self.active.clone() {
                Some(session) => toggle_session(&mut self.dock_state.research, &session),
                None => self.dock_state.draft_research = !self.dock_state.draft_research,
            }
            self.show_modes();
            return;
        }
        if which == "mode" && id == "harness:codemode" {
            match self.active.clone() {
                Some(session) => toggle_session(&mut self.dock_state.code_mode, &session),
                None => self.dock_state.draft_code_mode = !self.dock_state.draft_code_mode,
            }
            self.show_modes();
            return;
        }
        let Some(session) = self.active.clone() else {
            return;
        };
        let value = id.to_string();
        match which {
            "mode" if value == "harness:machine" => {
                self.send(Command::SetMachine { session, on: true });
            }
            "mode" => {
                if self.dock().get_machine() {
                    self.send(Command::SetMachine {
                        session: session.clone(),
                        on: false,
                    });
                }
                self.send(Command::SetMode {
                    session,
                    mode: value,
                });
            }
            "model" => match self.dock_state.model_id.clone() {
                Some(option) => self.send(Command::SetConfigOption {
                    session,
                    id: option,
                    value,
                }),
                None => self.send(Command::SetModel {
                    session,
                    model: value,
                }),
            },
            "effort" => {
                if let Some(option) = self.dock_state.effort_id.clone() {
                    self.send(Command::SetConfigOption {
                        session,
                        id: option,
                        value,
                    });
                }
            }
            _ => {}
        }
    }

    /// Skills and subagents of the open session's agent for the `@` popup (asked on open and on an agent change).
    pub(crate) fn load_skills(&self) {
        let Some(session) = self.active.clone() else {
            crate::chat::agent_mentions(Vec::new());
            return;
        };
        let query = MentionQuery {
            session_id: harness_core_api::SessionId::new(session.as_str()),
            query: String::new(),
            kinds: vec![MentionKind::Skill, MentionKind::Subagent],
            limit: 50,
        };
        let asked = session;
        self.chat_call(
            |reply| ChatCall::Mentions(query, reply),
            move |s, list| {
                if s.is_active(&asked) {
                    crate::chat::agent_mentions(list);
                }
            },
        );
    }
}
