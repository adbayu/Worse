use super::{
    ago, refresh, set_error, sv, with_ui, GhStatus, InboxSource, InboxSourceRow, InboxStatus,
    InboxTicketRow, InboxTriggerRow, Ticket, TicketStatus, TriggerRecord,
};
use std::fmt::Write as _;
pub(super) fn gh_text(status: &InboxStatus) -> String {
    match status.gh {
        GhStatus::Missing => {
            "GitHub CLI (gh) was not found. Harness can download the official release for you."
                .into()
        }
        GhStatus::SignedOut => format!(
            "gh is installed but not signed in. Sign in yourself by running this in a terminal: {}",
            gh_command(status)
        ),
        GhStatus::Ready if status.gh_detail.is_empty() => "gh is ready.".into(),
        GhStatus::Ready => format!("Signed in to GitHub as {}.", status.gh_detail),
    }
}

pub(super) fn gh_command(status: &InboxStatus) -> String {
    let path = if status.gh_path.is_empty() {
        "gh".to_owned()
    } else {
        format!("\"{}\"", status.gh_path)
    };
    format!("{path} auth login --web -s project")
}

pub fn gh_install_event(status: harness_core_api::AdapterInstallStatus, message: Option<String>) {
    if status == harness_core_api::AdapterInstallStatus::Running {
        with_ui(|ui| ui.set_gh_installing(true));
        return;
    }
    with_ui(|ui| ui.set_gh_installing(false));
    if status == harness_core_api::AdapterInstallStatus::Failed {
        set_error(&message.unwrap_or_default());
    }
    refresh();
}

pub(super) fn source_row(source: &InboxSource) -> InboxSourceRow {
    let users = if source.allowlist.is_empty() {
        "nobody allowed yet".to_owned()
    } else {
        source.allowlist.join(", ")
    };
    let every = format!("every {} min", source.interval_secs / 60);
    let (detail, failing) = if !source.enabled {
        (format!("Paused · {} from {users}", source.handle), false)
    } else if let Some(error) = &source.last_error {
        (format!("Last check failed: {error}"), true)
    } else {
        let checked = source.last_poll.map_or_else(
            || "not checked yet".to_owned(),
            |t| format!("checked {}", ago(t)),
        );
        (
            format!("{} from {users} · {every} · {checked}", source.handle),
            false,
        )
    };
    let mut detail = detail;
    if source.route {
        detail.push_str(" · routed by Orchestra");
    }
    if !source.shadow_labels.is_empty() {
        let _ = write!(detail, " · shadow: {}", source.shadow_labels.join(", "));
    }
    InboxSourceRow {
        id: sv(&source.id),
        repo: sv(&source.repo),
        folder: sv(&source.workspace_path),
        detail: sv(&detail),
        enabled: source.enabled,
        failing,
        has_router: source.router_session_id.is_some(),
    }
}

pub(super) fn ticket_row(ticket: &Ticket) -> InboxTicketRow {
    let (status, tone) = match ticket.status {
        TicketStatus::Working => ("Working", "progress"),
        TicketStatus::NeedsApproval => ("Needs approval", "warning"),
        TicketStatus::Review if ticket.shadow => ("Proposal ready", "accent"),
        TicketStatus::Review if ticket.pending_post => ("Draft ready", "accent"),
        TicketStatus::Review => ("In review", "success"),
        TicketStatus::Done => ("Done", "success"),
        TicketStatus::Failed => ("Failed", "destructive"),
        TicketStatus::Discarded => ("Discarded", "neutral"),
    };
    let has_session = ticket.session_id.is_some();
    InboxTicketRow {
        id: sv(&ticket.id),
        title: sv(&format!("#{} {}", ticket.number, ticket.title)),
        meta: sv(&format!(
            "{} · from {} · {}",
            ticket.repo,
            ticket.requester,
            ago(ticket.updated_at)
        )),
        status: sv(status),
        tone: sv(tone),
        pr: sv(ticket.pr_url.as_deref().unwrap_or_default()),
        error: sv(ticket.error.as_deref().unwrap_or_default()),
        has_session,
        can_post: has_session
            && (ticket.pending_post
                || ticket.status == TicketStatus::Failed
                || (ticket.shadow && ticket.status.is_active())),
        can_stop: matches!(
            ticket.status,
            TicketStatus::Working | TicketStatus::NeedsApproval
        ),
        can_discard: !matches!(ticket.status, TicketStatus::Done | TicketStatus::Discarded),
        shadow: ticket.shadow,
    }
}

pub(super) fn trigger_row(record: &TriggerRecord) -> InboxTriggerRow {
    InboxTriggerRow {
        title: sv(&format!(
            "{} on {}#{} ({})",
            record.author, record.repo, record.number, record.kind
        )),
        verdict: sv(&record.reason),
        accepted: record.accepted,
        time: sv(&ago(record.received_at)),
    }
}
