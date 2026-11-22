use super::{
    agy_agent, chosen_name, code_mode_item, matching_models, option_rows, sv, toggle_session,
    ChatCall, Command, ContextItem, DockUi, MentionKind, MentionQuery, ModelRc, Options, SessionId,
    SharedString, UiState,
};

/// Harness-side modes that stick to a session (or to the draft of a new chat).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct Modes {
    research: bool,
    prd: bool,
    code: bool,
}

/// Mode picker text: the agent's mode plus the sticky Harness modes that are on.
fn mode_name(label: &str, modes: Modes) -> String {
    let mut name = label.to_owned();
    let suffixes = [
        (modes.research, "Deep research", " · Research"),
        (modes.prd, "PRD mode", " · PRD"),
        (modes.code, "Code mode", " · Code"),
    ];
    for (on, bare, suffix) in suffixes {
        if !on {
            continue;
        }
        if name.is_empty() {
            name.push_str(bare);
        } else {
            name.push_str(suffix);
        }
    }
    name
}

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
            dock.set_prd(self.active.is_none() && self.dock_state.draft_prd);
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
        let state = &self.dock_state;
        let modes = self.active.as_ref().map_or(
            Modes {
                research: state.draft_research,
                prd: state.draft_prd,
                code: state.draft_code_mode,
            },
            |session| Modes {
                research: state.research.contains(session),
                prd: state.prd.contains(session),
                code: state.code_mode.contains(session),
            },
        );
        let dock = self.dock();
        dock.set_research(modes.research);
        dock.set_prd(modes.prd);
        dock.set_code_mode(modes.code);
        dock.set_code_mode_supported(!agy_agent(&self.sticky));
        dock.set_mode_name(sv(&mode_name(&self.dock_state.mode_label, modes)));
    }

    /// Adds the context items of the session's sticky modes (Deep research, PRD, Code mode) to a message.
    pub(crate) fn add_modes(&mut self, session: &SessionId, context: &mut Vec<ContextItem>) {
        self.add_research(session, context);
        self.add_prd(session, context);
        self.add_code_mode(session, context);
        self.add_open_work(context);
    }

    /// Moves the modes chosen for a new chat into its first message.
    pub(crate) fn take_draft_modes(&mut self, context: &mut Vec<ContextItem>) {
        if std::mem::take(&mut self.dock_state.draft_research) {
            context.push(ContextItem::Research);
        }
        if std::mem::take(&mut self.dock_state.draft_prd) {
            context.push(ContextItem::Prd);
        }
        self.take_draft_code_mode(context);
        self.add_open_work(context);
    }

    fn add_research(&mut self, session: &SessionId, context: &mut Vec<ContextItem>) {
        if context.contains(&ContextItem::Research) {
            self.dock_state.research.insert(session.clone());
            self.show_modes();
        } else if self.dock_state.research.contains(session) {
            context.push(ContextItem::Research);
        }
    }

    fn add_prd(&mut self, session: &SessionId, context: &mut Vec<ContextItem>) {
        if context.contains(&ContextItem::Prd) {
            self.dock_state.prd.insert(session.clone());
            self.show_modes();
        } else if self.dock_state.prd.contains(session) {
            context.push(ContextItem::Prd);
        }
    }

    fn add_code_mode(&mut self, session: &SessionId, context: &mut Vec<ContextItem>) {
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

    fn take_draft_code_mode(&mut self, context: &mut Vec<ContextItem>) {
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
        if which == "mode" && id == "harness:prd" {
            match self.active.clone() {
                Some(session) => toggle_session(&mut self.dock_state.prd, &session),
                None => self.dock_state.draft_prd = !self.dock_state.draft_prd,
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

#[cfg(test)]
mod tests {
    use super::{mode_name, Modes};

    const DOT: char = '·';

    fn m(research: bool, prd: bool, code: bool) -> Modes {
        Modes {
            research,
            prd,
            code,
        }
    }

    #[test]
    fn mode_name_lists_the_sticky_modes() {
        let none = Modes::default();
        assert_eq!(mode_name("Agent", none), "Agent");
        assert_eq!(
            mode_name("Agent", m(true, false, false)),
            format!("Agent {DOT} Research")
        );
        assert_eq!(
            mode_name("Agent", m(false, false, true)),
            format!("Agent {DOT} Code")
        );
        assert_eq!(
            mode_name("Agent", m(true, false, true)),
            format!("Agent {DOT} Research {DOT} Code")
        );
        assert_eq!(
            mode_name("Agent", m(false, true, false)),
            format!("Agent {DOT} PRD")
        );
        assert_eq!(mode_name("", none), "");
        assert_eq!(mode_name("", m(true, false, false)), "Deep research");
        assert_eq!(mode_name("", m(false, false, true)), "Code mode");
        assert_eq!(mode_name("", m(false, true, false)), "PRD mode");
        assert_eq!(
            mode_name("", m(true, false, true)),
            format!("Deep research {DOT} Code")
        );
    }
}
