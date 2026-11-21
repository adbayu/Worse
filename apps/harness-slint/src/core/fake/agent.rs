use super::{
    mpsc, oneshot, watch, AgentMsg, AgentOut, Duration, Job, SessionId, SystemTime, ToolStatus,
    UNIX_EPOCH,
};

pub(super) fn demo_subagents(parent: &SessionId) -> Vec<harness_core_api::SubagentView> {
    use harness_core_api::{AgentId, SessionId as ApiId, SubagentStatus, SubagentView};
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(0);
    let row =
        |n: u32, agent: &str, title: &str, status, activity: &str, preview: &str, ago: i64| {
            SubagentView {
                session_id: ApiId::new(format!("sess_demo{n}")),
                parent_id: ApiId::new(parent.clone()),
                agent: AgentId::new(agent.to_owned()),
                title: title.to_owned(),
                status,
                activity: activity.to_owned(),
                started_at: now - ago * 1000,
                finished_at: (status == SubagentStatus::Done).then_some(now - 20_000),
                result_preview: preview.to_owned(),
                scope: (n == 1).then(|| harness_core_api::ScopeView {
                    paths: vec!["src/core".to_owned()],
                    tools: vec!["edit".to_owned()],
                    network: false,
                    expires_at: Some(now + 600_000),
                }),
            }
        };
    vec![
        row(
            1,
            "deepseek",
            "Rewrite hub path claiming",
            SubagentStatus::Running,
            "edit src/core/hub.rs · +38 −12",
            "",
            221,
        ),
        row(
            2,
            "deepseek",
            "Backfill claim_paths tests",
            SubagentStatus::Running,
            "cargo check -p harness-core",
            "",
            140,
        ),
        row(
            3,
            "codex",
            "Port shell runner to tokio",
            SubagentStatus::NeedsApproval,
            "Run cargo test?",
            "",
            95,
        ),
        row(
            4,
            "agy",
            "Audit agent picker revert",
            SubagentStatus::Done,
            "",
            "No regressions found in agent_picker.slint.",
            400,
        ),
    ]
}

/// The markdown the `ZCode` reference capture gets from the mock provider (`P7_MD_*`, phase7-parity
/// mockProvider.mjs `MARKDOWN_REPLY`), so side-by-side screenshots compare the same content.
const PLAN_REPLY: &str = "## Plan\n\nI checked `src/main.rs` and the build. Steps:\n\n\
    1. Parse the config\n2. Start the **hub**\n3. Report the result\n\n\
    ```rust\nfn main() {\n    // start the hub\n    let port = 8080;\n    println!(\"listening on {port}\");\n}\n```\n\n\
    | Agent | State | Resets |\n| --- | --- | --- |\n| Claude Code | available | - |\n| Codex | limit reached | 18:00 |\n";

fn reply_body(job: &Job) -> String {
    if job.prompt.to_lowercase().contains("plan") {
        return PLAN_REPLY.to_string();
    }
    let short: String = job.prompt.chars().take(40).collect();
    format!(
        "## {name} reply\n\nYou asked: *{short}*. This is a **fake** {name} answer with `inline code`.\n\n\
         - streamed in small chunks\n- rendered as [markdown](https://example.com/harness)\n\n\
         ```rust\nfn main() {{\n    println!(\"hello from {id}\");\n}}\n```\n\nDone ({id}).",
        name = job.name,
        id = job.agent,
    )
}

/// Streams `text` in small chunks. Returns false when cancelled.
async fn stream(
    job: &Job,
    tx: &mpsc::UnboundedSender<AgentMsg>,
    cancel: &mut watch::Receiver<bool>,
    text: &str,
    delay: Duration,
) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(8) {
        if *cancel.borrow() {
            return false;
        }
        let piece: String = chunk.iter().collect();
        if tx
            .send(AgentMsg {
                run: job.run,
                session: job.session.clone(),
                out: AgentOut::Delta(piece),
            })
            .is_err()
        {
            return false;
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = cancel.changed() => return false,
        }
    }
    true
}

pub(super) async fn fake_agent(
    job: Job,
    tx: mpsc::UnboundedSender<AgentMsg>,
    mut cancel: watch::Receiver<bool>,
) {
    let send = |out: AgentOut| {
        tx.send(AgentMsg {
            run: job.run,
            session: job.session.clone(),
            out,
        })
        .is_ok()
    };
    let lower = job.prompt.to_lowercase();
    let fast = Duration::from_millis(15);
    let mut completed = true;
    if job.continuation {
        completed = stream(
            &job,
            &tx,
            &mut cancel,
            "_(continuing after an agent switch)_\n\n",
            fast,
        )
        .await;
    }
    if !completed {
    } else if lower.contains("permission") {
        send(AgentOut::ToolStart("Write notes.txt".into()));
        let (reply_tx, reply_rx) = oneshot::channel();
        send(AgentOut::Permission {
            title: "Allow Write?".into(),
            detail: format!("{} wants to write notes.txt in the workspace.", job.name),
            reply: reply_tx,
        });
        let allowed = tokio::select! {
            answer = reply_rx => answer.unwrap_or(false),
            _ = cancel.changed() => { send(AgentOut::Finished { cancelled: true }); return; }
        };
        send(AgentOut::ToolEnd(if allowed {
            ToolStatus::Done
        } else {
            ToolStatus::Denied
        }));
        let text = if allowed {
            "Permission **approved**: wrote `notes.txt`."
        } else {
            "Permission **denied**: nothing was written."
        };
        completed = stream(&job, &tx, &mut cancel, text, fast).await;
    } else if lower.contains("quota") && !job.continuation {
        let body = reply_body(&job);
        let half: String = body.chars().take(body.chars().count() / 2).collect();
        if stream(&job, &tx, &mut cancel, &half, fast).await {
            let reset = std::env::var("HARNESS_FAKE_RESET")
                .ok()
                .or_else(|| Some("10:00".into()));
            send(AgentOut::Exhausted { reset });
            return;
        }
        completed = false;
    } else if lower.contains("slow") {
        let long = "This is a deliberately slow streamed answer. ".repeat(200);
        completed = stream(&job, &tx, &mut cancel, &long, Duration::from_millis(80)).await;
    } else {
        send(AgentOut::ToolStart("Read README.md".into()));
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(60)) => {}
            _ = cancel.changed() => { send(AgentOut::Finished { cancelled: true }); return; }
        }
        send(AgentOut::ToolEnd(ToolStatus::Done));
        completed = stream(&job, &tx, &mut cancel, &reply_body(&job), fast).await;
    }
    send(AgentOut::Finished {
        cancelled: !completed,
    });
}
