//! Composer dock of 9.2B round 3 (spec `docs/specs/harness/phase-9-parity.md` §7.4 "UI contract for 9.2B round 3"):
//! queue edit / reorder / steer (R12), mode / model / effort pickers and skill / subagent mentions (R13) and the typed
//! error banner (R20).
//!
//! The hub owns the queue, the per-session choices and the failure. This module only mirrors
//! `Event::Queue` / `Event::SessionOptions` / `Event::Failure` into `DockUi` and turns clicks into commands; the
//! UI-local state is the queued message being edited and the retry countdown.
//!
//! ```text
//! click ─▶ DockUi callback ─▶ dock.rs ─▶ Command::{EditQueued, ReorderQueued, RunQueuedNow, Steer, Set*, RetryInit}
//!                                              │ core thread (hub session actor = owner)
//! DockUi ◀── dock.rs ◀── Event::{Queue, SessionOptions, Failure} ◀──────────────┘ (replace semantics)
//! ```

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use harness_core_api::{
    ContextItem, FailureKind, MentionKind, MentionQuery, ResumableRun, RetryHint,
};

use super::{with_state, UiState};
use crate::core::{ChatCall, Command, QueuedMessage, SessionId};
use crate::ui::{ChatUi, DockUi, OptionRow};

mod failure;
mod pickers;

/// UI-local dock state.
#[derive(Default)]
pub(super) struct DockState {
    /// Queued message whose text is in the composer; Send then sends `EditQueued`.
    editing: Option<String>,
    /// Text Copy puts on the clipboard (kind title is added by the banner reader).
    failure_copy: String,
    /// Session of the shown failure (`None` = the hub failed to start).
    failure_session: Option<SessionId>,
    /// Retry countdown of an `After` hint.
    countdown: Option<slint::Timer>,
    /// Config option id behind the effort picker (`SessionOptions.effort_id`).
    effort_id: Option<String>,
    /// Config option id behind the model picker (`SessionOptions.model_id`; `None` = `session/set_model`).
    model_id: Option<String>,
    /// Sessions in deep research mode: each composer message gets `ContextItem::Research`.
    research: std::collections::HashSet<SessionId>,
    /// Mode picker text without the research suffix.
    mode_label: String,
    /// Deep research chosen before the first message of a new chat.
    draft_research: bool,
    code_mode: std::collections::HashSet<SessionId>,
    draft_code_mode: bool,
    /// Sessions in PRD mode: each composer message gets `ContextItem::Prd`.
    prd: std::collections::HashSet<SessionId>,
    /// PRD mode chosen before the first message of a new chat.
    draft_prd: bool,
}

/// Picker values of one `Event::SessionOptions`.
pub struct Options {
    pub mode: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub effort_id: Option<String>,
    pub model_id: Option<String>,
    pub modes: Vec<(String, String)>,
    pub models: Vec<(String, String)>,
    pub efforts: Vec<(String, String)>,
    pub pending: bool,
    pub locked: bool,
    pub machine: bool,
}

/// One `Event::Failure`.
pub struct FailureEvent {
    pub session: Option<SessionId>,
    pub kind: FailureKind,
    pub message: String,
    pub details: Option<String>,
    pub trace: Option<String>,
    pub retry: RetryHint,
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn option_rows(list: &[(String, String)]) -> ModelRc<OptionRow> {
    ModelRc::new(VecModel::from(
        list.iter()
            .map(|(id, name)| OptionRow {
                id: sv(id),
                name: sv(if name.is_empty() { id } else { name }),
            })
            .collect::<Vec<_>>(),
    ))
}

/// The models whose name or id contains every word of `query` (case-insensitive); all of them for an empty
/// query.
fn matching_models(list: &[(String, String)], query: &str) -> Vec<(String, String)> {
    let query = query.to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    list.iter()
        .filter(|(id, name)| {
            let text = format!("{name} {id}").to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .cloned()
        .collect()
}

/// Display name of the chosen value; the id when the agent did not list it, "" when nothing is chosen.
fn chosen_name(list: &[(String, String)], value: Option<&str>) -> String {
    let Some(value) = value else {
        return list
            .first()
            .map(|(_, name)| name.clone())
            .unwrap_or_default();
    };
    list.iter()
        .find(|(id, _)| id == value)
        .map_or_else(|| value.to_string(), |(_, name)| name.clone())
}

fn kind_id(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Auth => "auth",
        FailureKind::ProcessExited => "processExited",
        FailureKind::InitFailed => "initFailed",
        FailureKind::Transient => "transient",
        FailureKind::Other => "other",
    }
}

fn agy_agent(agent: &str) -> bool {
    agent == crate::agy_usage::AGY_ID || agent.starts_with("agy-account-")
}

fn toggle_session(set: &mut std::collections::HashSet<SessionId>, session: &SessionId) {
    if !set.remove(session) {
        set.insert(session.clone());
    }
}

fn code_mode_item(enabled: bool, agy: bool) -> Option<ContextItem> {
    (enabled && !agy).then_some(ContextItem::CodeMode)
}

impl UiState {
    fn dock(&self) -> DockUi<'_> {
        self.window.global::<DockUi>()
    }

    /// A session was opened or left: no edit in progress, no pickers until its options arrive, no failure.
    pub(super) fn reset_dock(&mut self) {
        self.end_edit();
        self.apply_options(None);
        self.dismiss_failure();
        self.load_skills();
        self.load_resumable();
    }

    // ---- queue (R12) ---------------------------------------------------------------------------------

    fn end_edit(&mut self) {
        self.dock_state.editing = None;
        self.dock().set_editing_id(SharedString::new());
    }

    /// `Event::Queue` of the open session arrived (after `set_queue`): an edited message that left the queue
    /// (it started meanwhile) ends the edit mode; the draft stays so nothing typed is lost.
    pub(super) fn queue_changed(&mut self, items: &[QueuedMessage]) {
        if let Some(id) = &self.dock_state.editing {
            if !items.iter().any(|q| &q.id == id) {
                self.end_edit();
            }
        }
    }

    fn queue_action(&mut self, action: &str, id: &str) {
        let Some(session) = self.active.clone() else {
            return;
        };
        match action {
            "steer" => self.send(Command::RunQueuedNow {
                session,
                id: id.to_string(),
            }),
            "edit" => {
                let draft = self.window.get_draft();
                let chips = self.window.global::<ChatUi>().get_chips().row_count();
                if !draft.trim().is_empty() || chips > 0 {
                    let text = self
                        .window
                        .global::<ChatUi>()
                        .invoke_action_label(sv("editDraftConflict"));
                    self.window.set_error_text(text);
                    return;
                }
                let Some(text) = (0..self.queue.row_count())
                    .filter_map(|i| self.queue.row_data(i))
                    .find(|q| q.id == id)
                    .map(|q| q.text.to_string())
                else {
                    return;
                };
                self.dock_state.editing = Some(id.to_string());
                self.dock().set_editing_id(sv(id));
                crate::chat::restore_composer(&text, Vec::new());
            }
            "cancel-edit" => {
                self.end_edit();
                crate::chat::restore_composer("", Vec::new());
            }
            "remove" => self.send(Command::CancelQueued {
                session,
                id: id.to_string(),
            }),
            _ => {}
        }
    }

    /// Grip drag / accessible increment: moves `id` by `delta` rows and sends the whole order once.
    fn queue_move(&self, id: &str, delta: i32) {
        let Some(session) = self.active.clone() else {
            return;
        };
        let mut ids: Vec<String> = (0..self.queue.row_count())
            .filter_map(|i| self.queue.row_data(i))
            .map(|q| q.id.to_string())
            .collect();
        let Some(from) = ids.iter().position(|q| q == id) else {
            return;
        };
        let last = i64::try_from(ids.len()).unwrap_or(i64::MAX) - 1;
        let to = (i64::try_from(from).unwrap_or_default() + i64::from(delta)).clamp(0, last.max(0));
        let to = usize::try_from(to).unwrap_or_default();
        if to == from {
            return;
        }
        let moved = ids.remove(from);
        ids.insert(to, moved);
        self.send(Command::ReorderQueued { session, ids });
    }

    /// Send while editing a queued message: replaces its text (`EditQueued`). Returns false when not editing.
    pub(super) fn send_edit(&mut self, text: String) -> bool {
        let Some(id) = self.dock_state.editing.clone() else {
            return false;
        };
        self.end_edit();
        if let Some(session) = self.active.clone() {
            self.send(Command::EditQueued { session, id, text });
        }
        true
    }

    fn steer(&mut self) {
        let Some(session) = self.active.clone() else {
            return;
        };
        let draft = self.window.get_draft().to_string();
        let (text, mut context) = crate::chat::prepare_send(&draft);
        if text.trim().is_empty() && context.is_empty() {
            return;
        }
        self.add_modes(&session, &mut context);
        self.window.set_draft(SharedString::new());
        self.send(Command::Steer {
            session,
            agent: self.sticky.clone(),
            text,
            context,
        });
    }
}

pub(super) fn wire(window: &crate::ui::AppWindow) {
    let dock = window.global::<DockUi>();
    dock.on_queue_action(|action, id| with_state(|s| s.queue_action(&action, &id)));
    dock.on_queue_move(|id, delta| with_state(|s| s.queue_move(&id, delta)));
    dock.on_steer(|| with_state(UiState::steer));
    dock.on_pick_option(|which, id| with_state(|s| s.pick_option(&which, &id)));
    let weak = window.as_weak();
    dock.on_filter_models(move |query| {
        use slint::Model as _;
        let Some(window) = weak.upgrade() else {
            return;
        };
        let dock = window.global::<DockUi>();
        let all: Vec<(String, String)> = dock
            .get_models()
            .iter()
            .map(|row| (row.id.to_string(), row.name.to_string()))
            .collect();
        dock.set_models_shown(option_rows(&matching_models(&all, &query)));
    });
    dock.on_failure_action(|action| with_state(|s| s.failure_action(&action)));
    dock.on_resume_action(|action| with_state(|s| s.resume_action(&action)));
}

#[cfg(test)]
mod tests {
    use super::{agy_agent, code_mode_item, toggle_session};

    use harness_core_api::ContextItem;

    #[test]
    fn only_agy_agents_lack_code_mode() {
        assert!(agy_agent("agy"));
        assert!(agy_agent("agy-account-work"));
        assert!(!agy_agent("claude"));
        assert!(!agy_agent(""));
    }

    #[test]
    fn toggling_code_mode_is_sticky_per_session() {
        let mut set = std::collections::HashSet::new();
        toggle_session(&mut set, &"s1".to_owned());
        assert!(set.contains("s1"));
        toggle_session(&mut set, &"s2".to_owned());
        assert!(set.contains("s1"));
        assert!(set.contains("s2"));
        toggle_session(&mut set, &"s1".to_owned());
        assert!(!set.contains("s1"));
        assert!(set.contains("s2"));
    }

    #[test]
    fn code_mode_is_kept_but_not_pushed_for_agy() {
        assert_eq!(code_mode_item(true, false), Some(ContextItem::CodeMode));
        assert_eq!(code_mode_item(true, true), None);
        assert_eq!(code_mode_item(false, false), None);
    }
}
