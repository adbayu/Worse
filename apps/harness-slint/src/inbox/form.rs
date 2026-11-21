use super::{
    agent_label, agent_options, call, gh_command, model, now_ms, refresh, render_notices,
    set_error, sv, window, with_ui, ApiError, ChatCall, Command, ComponentHandle, InboxSource,
    InboxSourceInput, InboxUi, SelectOption, SharedString, STATE,
};

fn source(id: &str) -> Option<InboxSource> {
    STATE.with(|s| {
        s.borrow()
            .status
            .sources
            .iter()
            .find(|x| x.id == id)
            .cloned()
    })
}

fn input_from(source: &InboxSource) -> InboxSourceInput {
    InboxSourceInput {
        id: Some(source.id.clone()),
        repo: source.repo.clone(),
        workspace_path: source.workspace_path.clone(),
        workspace_key: source.workspace_key.clone(),
        enabled: source.enabled,
        handle: source.handle.clone(),
        allowlist: source.allowlist.clone(),
        interval_secs: source.interval_secs,
        agent: source.agent.clone(),
        draft_first: source.draft_first,
        max_turns: source.max_turns,
        route: source.route,
        shadow_labels: source.shadow_labels.clone(),
    }
}

fn open_form(source: Option<&InboxSource>) {
    let options: Vec<SelectOption> = agent_options()
        .into_iter()
        .map(|(id, label)| SelectOption {
            id: sv(&id),
            label: sv(&label),
        })
        .collect();
    with_ui(|ui| {
        ui.set_agents(model(options));
        ui.set_form_error(SharedString::new());
        if let Some(s) = source {
            let agent = s
                .agent
                .as_ref()
                .map(|a| a.as_str().to_owned())
                .unwrap_or_default();
            ui.set_form_id(sv(&s.id));
            ui.set_form_repo(sv(&s.repo));
            ui.set_form_folder(sv(&s.workspace_path));
            ui.set_form_users(sv(&s.allowlist.join(", ")));
            ui.set_form_handle(sv(&s.handle));
            ui.set_form_agent_label(sv(&agent_label(&agent)));
            ui.set_form_agent(sv(&agent));
            ui.set_form_minutes(i32::try_from(s.interval_secs / 60).unwrap_or(2));
            ui.set_form_draft(s.draft_first);
            ui.set_form_enabled(s.enabled);
            ui.set_form_route(s.route);
            ui.set_form_shadow(sv(&s.shadow_labels.join(", ")));
        } else {
            ui.set_form_id(SharedString::new());
            ui.set_form_repo(SharedString::new());
            ui.set_form_folder(sv(&crate::bridge::workspace_path()));
            ui.set_form_users(SharedString::new());
            ui.set_form_handle(sv(harness_core_api::DEFAULT_INBOX_HANDLE));
            ui.set_form_agent(SharedString::new());
            ui.set_form_agent_label(sv("Default agent"));
            ui.set_form_minutes(2);
            ui.set_form_draft(false);
            ui.set_form_enabled(true);
            ui.set_form_route(false);
            ui.set_form_shadow(SharedString::new());
        }
        ui.set_form_open(true);
    });
}

pub(super) fn save_form() {
    let Some(w) = window() else {
        return;
    };
    let ui = w.global::<InboxUi>();
    let id = ui.get_form_id().to_string();
    let previous = source(&id);
    let agent = ui.get_form_agent().to_string();
    let minutes = u32::try_from(ui.get_form_minutes().max(1)).unwrap_or(2);
    let input = InboxSourceInput {
        id: (!id.is_empty()).then_some(id),
        repo: ui.get_form_repo().trim().to_owned(),
        workspace_path: ui.get_form_folder().trim().to_owned(),
        workspace_key: previous
            .as_ref()
            .map(|p| p.workspace_key.clone())
            .unwrap_or_default(),
        enabled: ui.get_form_enabled(),
        handle: ui.get_form_handle().trim().to_owned(),
        allowlist: ui
            .get_form_users()
            .split([',', ' ', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        interval_secs: minutes.saturating_mul(60),
        agent: (!agent.is_empty()).then(|| agent.as_str().into()),
        draft_first: ui.get_form_draft(),
        max_turns: previous.as_ref().map_or(0, |p| p.max_turns),
        route: ui.get_form_route(),
        shadow_labels: ui
            .get_form_shadow()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    };
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::bridge::send(Command::Chat(ChatCall::SaveInboxSource(input, reply)));
    let _ = slint::spawn_local(async move {
        match answer.await.unwrap_or(Err(ApiError::ShutDown)) {
            Ok(_) => {
                with_ui(|ui| ui.set_form_open(false));
                refresh();
            }
            Err(error) => with_ui(|ui| ui.set_form_error(sv(&error.to_string()))),
        }
    });
}

pub(super) fn action_for(action: &str, id: &str) {
    let id = id.to_owned();
    let refreshed = |()| refresh();
    match action {
        "filter" => {
            STATE.with(|s| s.borrow_mut().filter.clone_from(&id));
            render_notices();
        }
        "mark-read" => {
            STATE.with(|s| s.borrow_mut().read_at = now_ms());
            render_notices();
        }
        "open-notice" => action_for("open-session", &id),
        "add" => open_form(None),
        "install-gh" => {
            set_error("");
            with_ui(|ui| ui.set_gh_installing(true));
            crate::bridge::send(Command::Setup(crate::core::SetupCall::Install(
                "gh".to_owned(),
            )));
        }
        "copy-gh" => {
            let command = STATE.with(|s| gh_command(&s.borrow().status));
            if let Some(w) = window() {
                w.invoke_copy_text(sv(&command));
            }
        }
        "edit" => open_form(source(&id).as_ref()),
        "agent-label" => with_ui(|ui| ui.set_form_agent_label(sv(&agent_label(&id)))),
        "enable" | "disable" => {
            if let Some(mut input) = source(&id).as_ref().map(input_from) {
                input.enabled = action == "enable";
                call(
                    |reply| ChatCall::SaveInboxSource(input, reply),
                    |_| refresh(),
                );
            }
        }
        "remove" => call(|reply| ChatCall::DeleteInboxSource(id, reply), refreshed),
        "poll" => {
            with_ui(|ui| ui.set_busy(sv(&id)));
            let (reply, answer) = tokio::sync::oneshot::channel();
            crate::bridge::send(Command::Chat(ChatCall::PollInboxNow(id, reply)));
            let _ = slint::spawn_local(async move {
                let result = answer.await.unwrap_or(Err(ApiError::ShutDown));
                with_ui(|ui| ui.set_busy(SharedString::new()));
                match result {
                    Ok(()) => set_error(""),
                    Err(error) => set_error(&error.to_string()),
                }
                refresh();
            });
        }
        "open-session" => {
            let session = STATE.with(|s| {
                s.borrow()
                    .tickets
                    .iter()
                    .find(|t| t.id == id)
                    .and_then(|t| t.session_id.clone())
            });
            if let Some(session) = session {
                crate::bridge::open_session(session.as_str());
            }
        }
        "open-router" => {
            if let Some(session) = source(&id).and_then(|s| s.router_session_id) {
                crate::bridge::open_session(session.as_str());
            }
        }
        "post" => call(|reply| ChatCall::PostTicket(id, reply), refreshed),
        "stop" => call(|reply| ChatCall::StopTicket(id, reply), refreshed),
        "discard" => call(|reply| ChatCall::DiscardTicket(id, reply), refreshed),
        _ => {}
    }
}
