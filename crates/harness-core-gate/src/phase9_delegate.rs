//! Orchestration (phase-9-parity.md §9.8): the gate plays the parent agent's MCP client, driving
//! `harness-memory-mcp --delegate` with the env the hub handed the fake agent, and checks subagent sessions,
//! `SubagentUpdated` events, follow-ups, rejection and stop propagation.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use harness_core_api::{EventKind, HarnessCore, ListSessions, SessionId, SubagentStatus};
use serde_json::{json, Value};

use super::phase9::{hub_config, start};
use super::{check, step, Env, Gate, A, B, R};

mod mcp;

pub(super) use mcp::{is_error, text, Client, Mcp};

pub(super) async fn run(env: &Env, base: &Path) -> R<()> {
    let home = base.join("delegate-data");
    std::fs::create_dir_all(&home).map_err(|e| format!("mkdir: {e}"))?;
    let gate = start(hub_config(env, &home, false)).await?;
    let parent = gate.session(env, A, "orchestrator").await?;
    let t = gate.send(&parent, "hello", None).await?;
    gate.completed(&parent, &t).await?;
    let vars = delegate_env(env, &parent)?;
    let exe = env.exe("harness-memory-mcp");
    let mcp = Client(std::sync::Arc::new(std::sync::Mutex::new(
        tokio::task::spawn_blocking(move || Mcp::start(&exe, &vars))
            .await
            .map_err(|e| e.to_string())??,
    )));
    let tools = mcp
        .blocking(|m| m.request("tools/list", &json!({})))
        .await?;
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();
    check(
        names
            == [
                "list_agents",
                "delegate",
                "subagent_message",
                "subagent_wait",
                "claim_paths",
                "release_paths",
            ],
        format!("delegate tools {names:?}"),
    )?;
    let agents = mcp.call("list_agents", json!({})).await?;
    check(
        !is_error(&agents) && text(&agents).contains(B),
        format!("list_agents: {agents}"),
    )?;

    // 日志里还有前面步骤的 fake-b 顶层会话：只检查委派开始之后的条目。
    let mark = env.log_entries().len();
    let first = mcp
        .call(
            "delegate",
            json!({"agent": B, "title": "Gate child", "task": "[[tools]] [[say CHILD-ANSWER]]"}),
        )
        .await?;
    let body = text(&first);
    check(
        !is_error(&first) && body.contains("CHILD-ANSWER") && body.contains(" done"),
        format!("delegate result: {first}"),
    )?;
    let child = SessionId::new(
        body.split_whitespace()
            .nth(1)
            .ok_or("no session id in the delegate result")?,
    );
    child_checks(&gate, env, &parent, &child, mark).await?;

    let again = mcp
        .call(
            "delegate",
            json!({"session_id": child.as_str(), "task": "[[say SECOND]]"}),
        )
        .await?;
    check(
        !is_error(&again)
            && text(&again).contains(child.as_str())
            && text(&again).contains("SECOND"),
        format!("follow-up: {again}"),
    )?;
    let foreign = mcp
        .call(
            "delegate",
            json!({"session_id": parent.as_str(), "task": "x"}),
        )
        .await?;
    check(is_error(&foreign), "a non-subagent session_id was accepted")?;
    super::phase9_coord::run(&gate, env, &mcp, &parent).await?;
    wrong_token(env, &parent)?;
    stop_propagates(&gate, mcp, &parent).await?;
    gate.hub.shutdown().await.map_err(|e| e.to_string())?;
    step("delegate");
    Ok(())
}

async fn child_checks(
    gate: &Gate,
    env: &Env,
    parent: &SessionId,
    child: &SessionId,
    mark: usize,
) -> R<()> {
    let listed = gate
        .hub
        .list_sessions(ListSessions::default())
        .await
        .map_err(|e| e.to_string())?;
    check(
        listed.iter().any(|s| &s.id == parent) && !listed.iter().any(|s| &s.id == child),
        "subagent session listed as a top-level session",
    )?;
    let detail = gate
        .hub
        .open_session(child.clone())
        .await
        .map_err(|e| e.to_string())?;
    check(
        detail.summary.parent_id.as_ref() == Some(parent) && detail.summary.title == "Gate child",
        format!("child summary {:?}", detail.summary),
    )?;
    let parent_detail = gate
        .hub
        .open_session(parent.clone())
        .await
        .map_err(|e| e.to_string())?;
    let view = parent_detail
        .subagents
        .iter()
        .find(|s| &s.session_id == child)
        .ok_or("subagent missing from the parent detail")?;
    check(
        view.status == SubagentStatus::Done
            && view.agent.as_str() == B
            && view.result_preview.contains("CHILD-ANSWER")
            && !view.activity.is_empty()
            && view.finished_at.is_some(),
        format!("subagent view {view:?}"),
    )?;
    let order: Vec<SubagentStatus> = gate
        .events()
        .iter()
        .filter(|e| e.session_id.as_ref() == Some(parent))
        .filter_map(|e| match &e.kind {
            EventKind::SubagentUpdated { subagent } if &subagent.session_id == child => {
                Some(subagent.status)
            }
            _ => None,
        })
        .collect();
    check(
        order.first() == Some(&SubagentStatus::Running)
            && order.last() == Some(&SubagentStatus::Done),
        format!("SubagentUpdated order {order:?}"),
    )?;
    let b_news: Vec<Value> = env
        .log_entries()
        .into_iter()
        .skip(mark)
        .filter(|e| e["event"] == "new" && e["agent"] == B)
        .collect();
    // §9.8.1：子会话可以拿到 harness-delegate，但必须是 child 角色。
    check(
        !b_news.is_empty()
            && b_news.iter().all(|e| {
                e["mcpServers"].as_array().is_some_and(|a| {
                    a.iter()
                        .filter(|s| s["name"] == "harness-delegate")
                        .all(|s| {
                            s["env"].as_array().is_some_and(|v| {
                                v.iter().any(|x| {
                                    x["name"] == "HARNESS_DELEGATE_ROLE" && x["value"] == "child"
                                })
                            })
                        })
                })
            }),
        "a subagent session was given harness-delegate in the parent role",
    )?;
    Ok(())
}

/// `harness-delegate` env of the parent's native session, from the fake agent log.
pub(super) fn delegate_env(env: &Env, parent: &SessionId) -> R<Vec<(String, String)>> {
    for entry in env.log_entries().iter().rev() {
        let Some(servers) = entry["mcpServers"].as_array() else {
            continue;
        };
        for server in servers.iter().filter(|s| s["name"] == "harness-delegate") {
            let vars: Vec<(String, String)> = server["env"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| {
                            Some((
                                v["name"].as_str()?.to_owned(),
                                v["value"].as_str()?.to_owned(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let session = vars.iter().find(|(k, _)| k == "HARNESS_DELEGATE_SESSION");
            if session.is_some_and(|(_, v)| v == parent.as_str()) {
                check(
                    server["args"] == json!(["--delegate"]),
                    format!("delegate server args {}", server["args"]),
                )?;
                return Ok(vars);
            }
        }
    }
    Err("no harness-delegate server was passed for the parent session".into())
}

fn wrong_token(env: &Env, parent: &SessionId) -> R<()> {
    let vars = delegate_env(env, parent)?;
    let addr = vars
        .iter()
        .find(|(k, _)| k == "HARNESS_DELEGATE_ADDR")
        .map(|(_, v)| v.clone())
        .ok_or("no delegate address")?;
    let mut stream = std::net::TcpStream::connect(&addr).map_err(|e| e.to_string())?;
    let request = json!({"token": "wrong", "parent": parent.as_str(), "op": "agents"});
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    check(
        reply["ok"] == false && reply.get("result").is_none(),
        format!("wrong token reply {reply}"),
    )
}

/// Stopping the parent session leaves a running subagent alone: it finishes and the delegate call returns
/// its answer.
async fn stop_propagates(gate: &Gate, mcp: Client, parent: &SessionId) -> R<()> {
    let call = tokio::spawn(async move {
        let result = mcp
            .call("delegate", json!({"agent": B, "task": "[[slow 60]]"}))
            .await;
        let _ = mcp
            .blocking(|m| {
                m.close();
                Ok(Value::Null)
            })
            .await;
        result
    });
    let started = gate
        .wait_event("slow subagent running", |e| {
            e.session_id.as_ref() == Some(parent)
                && matches!(&e.kind, EventKind::SubagentUpdated { subagent }
                    if subagent.status == SubagentStatus::Running && subagent.title == "[[slow 60]]")
        })
        .await?;
    let EventKind::SubagentUpdated { subagent } = started.kind else {
        return Err("unexpected event".into());
    };
    gate.hub
        .stop(parent.clone())
        .await
        .map_err(|e| e.to_string())?;
    let result = call.await.map_err(|e| e.to_string())??;
    check(
        !is_error(&result),
        format!("subagent survived parent stop {result}"),
    )?;
    let view = gate
        .hub
        .open_session(parent.clone())
        .await
        .map_err(|e| e.to_string())?
        .subagents
        .into_iter()
        .find(|s| s.session_id == subagent.session_id);
    check(
        view.is_some_and(|s| s.status == SubagentStatus::Done),
        "subagent done after parent stop",
    )?;
    Ok(())
}
