use super::{
    agent_label, ago, model, sv, with_ui, InboxNotice, SharedString, State, TicketStatus, STATE,
};

fn rgb(hex: u32) -> slint::Color {
    slint::Color::from_argb_encoded(0xFF00_0000 | hex)
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    match words.as_slice() {
        [] => "◎".into(),
        [one] if one.chars().count() <= 3 => (*one).to_owned(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, b, ..] => format!(
            "{}{}",
            a.chars().next().unwrap_or_default(),
            b.chars().next().unwrap_or_default()
        )
        .to_uppercase(),
    }
}

/// One design S4 inbox row: a ticket (work by a subagent) or a GitHub trigger (pull request / mention).
struct Notice {
    row: InboxNotice,
    at: i64,
    subagent: bool,
    build: bool,
}

fn notices(s: &State) -> Vec<Notice> {
    let mut out = Vec::new();
    for t in &s.tickets {
        let agent = s
            .status
            .sources
            .iter()
            .find(|x| x.id == t.source_id)
            .and_then(|x| x.agent.as_ref())
            .map(|a| a.as_str().to_owned())
            .unwrap_or_default();
        let name = if agent.is_empty() {
            "Agent".to_owned()
        } else {
            agent_label(&agent)
        };
        let item = format!("#{} {}", t.number, t.title);
        let (title, pill, tint) = match t.status {
            TicketStatus::Working => (format!("{name} working: {item}"), "working", 0x7C_8CFF),
            TicketStatus::NeedsApproval => (
                format!("{name} needs permission: {item}"),
                "waiting",
                0xF5_B14C,
            ),
            TicketStatus::Review if t.shadow => {
                (format!("Proposal ready: {item}"), "proposal", 0x5B_C0FF)
            }
            TicketStatus::Review if t.pending_post => {
                (format!("Draft ready: {item}"), "draft", 0x5B_C0FF)
            }
            TicketStatus::Review => (
                format!("{name} opened a pull request: {item}"),
                "review",
                0x5B_C0FF,
            ),
            TicketStatus::Done => (format!("{name} finished: {item}"), "done", 0x3D_D68C),
            TicketStatus::Failed => (format!("{name} failed: {item}"), "failed", 0xF2_555A),
            TicketStatus::Discarded => (format!("Discarded: {item}"), "", 0x6B_717C),
        };
        let chip_tint = if agent.is_empty() {
            rgb(0x7C_8CFF)
        } else {
            crate::orchestra::agent_color(&agent)
        };
        out.push(Notice {
            row: InboxNotice {
                id: sv(&t.id),
                chip: sv(&initials(&name)),
                tint: chip_tint,
                title: sv(&title),
                meta: sv(&format!("{} · {}", t.repo, ago(t.updated_at))),
                pill: sv(pill),
                pill_tint: rgb(tint),
                trailing: SharedString::new(),
                unread: t.updated_at > s.read_at,
                can_open: t.session_id.is_some(),
            },
            at: t.updated_at,
            subagent: true,
            build: false,
        });
    }
    for r in &s.triggers {
        let pr = r.kind.contains("pull") || r.kind.contains("review");
        out.push(Notice {
            row: InboxNotice {
                id: sv(&r.id),
                chip: sv("i"),
                tint: rgb(0x5B_C0FF),
                title: sv(&format!(
                    "{}#{} {} by {}",
                    r.repo, r.number, r.kind, r.author
                )),
                meta: sv(&format!("{} · {}", r.repo, ago(r.received_at))),
                pill: sv(if !r.accepted {
                    "ignored"
                } else if pr {
                    "review"
                } else {
                    "mention"
                }),
                pill_tint: rgb(if r.accepted { 0x5B_C0FF } else { 0x6B_717C }),
                trailing: SharedString::new(),
                unread: r.received_at > s.read_at,
                can_open: false,
            },
            at: r.received_at,
            subagent: false,
            build: false,
        });
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.at));
    out
}

pub(super) fn render_notices() {
    let (rows, all, unread, subagents, builds, filter) = STATE.with(|cell| {
        let s = cell.borrow();
        let list = notices(&s);
        let count = |f: &dyn Fn(&Notice) -> bool| {
            i32::try_from(list.iter().filter(|n| f(n)).count()).unwrap_or(i32::MAX)
        };
        let (all, unread, subagents, builds) = (
            count(&|_| true),
            count(&|n| n.row.unread),
            count(&|n| n.subagent),
            count(&|n| n.build),
        );
        let filter = if s.filter.is_empty() {
            "all".to_owned()
        } else {
            s.filter.clone()
        };
        let rows: Vec<InboxNotice> = list
            .into_iter()
            .filter(|n| match filter.as_str() {
                "unread" => n.row.unread,
                "subagents" => n.subagent,
                "builds" => n.build,
                _ => true,
            })
            .map(|n| n.row)
            .collect();
        (rows, all, unread, subagents, builds, filter)
    });
    with_ui(|ui| {
        ui.set_notices(model(rows));
        ui.set_count_all(all);
        ui.set_count_unread(unread);
        ui.set_count_subagents(subagents);
        ui.set_count_builds(builds);
        ui.set_filter(sv(&filter));
    });
}
