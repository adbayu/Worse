use std::cell::RefCell;

use harness_core_api::{
    ApiError, GhStatus, InboxSource, InboxSourceInput, InboxStatus, Ticket, TicketStatus,
    TriggerRecord,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

use crate::core::{ChatCall, Command, Reply};
use crate::ui::{
    AppWindow, InboxNotice, InboxSourceRow, InboxTicketRow, InboxTriggerRow, InboxUi, SelectOption,
    SettingsUi,
};

mod form;
mod notices;
mod rows;

use form::{action_for, save_form};
use notices::render_notices;
pub use rows::gh_install_event;
use rows::{gh_command, gh_text, source_row, ticket_row, trigger_row};

#[derive(Default)]
struct State {
    window: Option<Weak<AppWindow>>,
    status: InboxStatus,
    tickets: Vec<Ticket>,
    triggers: Vec<TriggerRecord>,
    /// Notices updated before this instant count as read (`Mark all read`).
    read_at: i64,
    /// Inbox list filter: all | unread | subagents | builds.
    filter: String,
    pending: Option<slint::Timer>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(rows)))
}

fn window() -> Option<AppWindow> {
    STATE.with(|s| s.borrow().window.as_ref().and_then(Weak::upgrade))
}

fn with_ui(f: impl FnOnce(InboxUi<'_>)) {
    if let Some(w) = window() {
        f(w.global::<InboxUi>());
    }
}

fn set_error(text: &str) {
    with_ui(|ui| ui.set_error(sv(text)));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn ago(ms: i64) -> String {
    let secs = (now_ms() - ms).max(0) / 1000;
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86_400 {
        format!("{} h ago", secs / 3600)
    } else {
        format!("{} d ago", secs / 86_400)
    }
}

fn call<T: 'static>(make: impl FnOnce(Reply<T>) -> ChatCall, done: impl FnOnce(T) + 'static) {
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::bridge::send(Command::Chat(make(reply)));
    let _ = slint::spawn_local(async move {
        match answer.await.unwrap_or(Err(ApiError::ShutDown)) {
            Ok(value) => done(value),
            Err(error) => set_error(&error.to_string()),
        }
    });
}

fn agent_options() -> Vec<(String, String)> {
    let mut out = vec![(String::new(), "Default agent".to_owned())];
    out.extend(
        crate::bridge::orchestra_agents()
            .into_iter()
            .filter(|a| a.2)
            .map(|a| (a.0, a.1)),
    );
    out
}

fn agent_label(id: &str) -> String {
    agent_options()
        .into_iter()
        .find(|a| a.0 == id)
        .map_or_else(|| id.to_owned(), |a| a.1)
}

pub fn install(window: &AppWindow) {
    STATE.with(|s| s.borrow_mut().window = Some(window.as_weak()));
    let ui = window.global::<InboxUi>();
    ui.on_set_enabled(|on| {
        with_ui(|ui| ui.set_enabled(on));
        call(
            |reply| ChatCall::SetInboxEnabled(on, reply),
            |()| {
                set_error("");
                refresh();
            },
        );
    });
    ui.on_action(|action, id| action_for(&action, &id));
    ui.on_save_form(save_form);
}

pub fn open() {
    set_error("");
    refresh();
}

pub fn changed() {
    let Some(w) = window() else {
        return;
    };
    if w.get_settings_open() && w.global::<SettingsUi>().get_page() == "inbox" {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.pending.is_none() {
                let timer = slint::Timer::default();
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(150),
                    || {
                        STATE.with(|s| s.borrow_mut().pending = None);
                        refresh();
                    },
                );
                s.pending = Some(timer);
            }
        });
    }
}

fn refresh() {
    call(ChatCall::InboxStatus, |status: InboxStatus| {
        with_ui(|ui| {
            ui.set_enabled(status.enabled);
            ui.set_gh_ready(status.gh == GhStatus::Ready);
            ui.set_gh_text(sv(&gh_text(&status)));
            ui.set_gh_state(match status.gh {
                GhStatus::Ready => 0,
                GhStatus::Missing => 1,
                GhStatus::SignedOut => 2,
            });
            ui.set_sources(model(status.sources.iter().map(source_row).collect()));
        });
        STATE.with(|s| s.borrow_mut().status = status);
    });
    call(ChatCall::Tickets, |tickets: Vec<Ticket>| {
        with_ui(|ui| ui.set_tickets(model(tickets.iter().map(ticket_row).collect())));
        STATE.with(|s| s.borrow_mut().tickets = tickets);
        render_notices();
    });
    call(
        |reply| ChatCall::Triggers(50, reply),
        |list: Vec<TriggerRecord>| {
            with_ui(|ui| ui.set_triggers(model(list.iter().map(trigger_row).collect())));
            STATE.with(|s| s.borrow_mut().triggers = list);
            render_notices();
        },
    );
}
