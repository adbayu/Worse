use super::{
    json, store, AcpConnection, ActorMsg, ActorTx, AgentErrorInfo, AgentId, Arc, HubCtx, PrefPlan,
    Prefs, Ready, RouteSink, TurnId, Value,
};

/// Takes the turn's efore snapshot and stores it (never fails the turn: errors are recorded).
pub(super) async fn take_before_snapshot(ctx: &Arc<HubCtx>, turn_id: &TurnId, workspace: &str) {
    let tmp = ctx.config.home.join("tmp");
    let result = crate::snapshot::take(&tmp, std::path::Path::new(workspace)).await;
    let (tree, error) = match result {
        Ok(Some(tree)) => (Some(tree), None),
        Ok(None) => (None, Some("workspace is not a git repository".to_owned())),
        Err(error) => {
            tracing::warn!(turn = %turn_id, %error, "turn snapshot (before) failed");
            (None, Some(error))
        }
    };
    let turn_id = turn_id.clone();
    if let Err(error) = ctx
        .store
        .run(move |c| store::set_snapshot_before(c, &turn_id, tree.as_deref(), error.as_deref()))
        .await
    {
        tracing::warn!(%error, "storing turn snapshot failed");
    }
}

/// Takes the turn's fter snapshot when a efore exists.
pub(super) async fn take_after_snapshot(ctx: &Arc<HubCtx>, turn_id: &TurnId, workspace: &str) {
    let lookup = turn_id.clone();
    let before = ctx
        .store
        .run(move |c| store::load_snapshot(c, &lookup))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.before_tree);
    if before.is_none() {
        return;
    }
    let tmp = ctx.config.home.join("tmp");
    let (tree, error) = match crate::snapshot::take(&tmp, std::path::Path::new(workspace)).await {
        Ok(tree) => (tree, None),
        Err(error) => {
            tracing::warn!(turn = %turn_id, %error, "turn snapshot (after) failed");
            (None, Some(error))
        }
    };
    let turn_id = turn_id.clone();
    if let Err(error) = ctx
        .store
        .run(move |c| store::set_snapshot_after(c, &turn_id, tree.as_deref(), error.as_deref()))
        .await
    {
        tracing::warn!(%error, "storing turn snapshot failed");
    }
}

/// Acquires the agent process and establishes the native session (new, reused or loaded).
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive_setup(
    ctx: &Arc<HubCtx>,
    tx: &ActorTx,
    agent: &AgentId,
    handle: Option<(String, Option<u64>)>,
    cwd: &str,
    plan: PrefPlan,
    session: &harness_core_api::SessionId,
    code_mode: bool,
) -> Result<Ready, AgentErrorInfo> {
    crate::agy_agents::ensure_account(ctx, agent)
        .await
        .map_err(|message| AgentErrorInfo {
            code: None,
            message,
            data: None,
        })?;
    let connection = ctx
        .pool
        .acquire(agent, code_mode)
        .await
        .map_err(|message| AgentErrorInfo {
            code: None,
            message,
            data: None,
        })?;
    let sink: RouteSink = {
        let tx = tx.clone();
        Arc::new(move |inbound| {
            let _ = tx.send(ActorMsg::Inbound(inbound));
        })
    };
    let mcp_servers = ctx
        .mcp_servers(agent, cwd, connection.mcp_capabilities(), session)
        .await;
    // (native, fresh, advertisement, choices already in effect)
    let mut established: Option<(String, bool, Option<Value>, Prefs)> = None;
    if let Some((native, generation)) = handle {
        if generation == Some(connection.generation()) {
            connection.register_route(&native, sink.clone());
            let applied = plan.applied.clone().unwrap_or_default();
            established = Some((native, false, None, applied));
        } else if connection.supports_load() {
            let loaded = connection
                .call(
                    "session/load",
                    json!({"sessionId": native, "cwd": cwd, "mcpServers": mcp_servers}),
                )
                .await;
            if let Ok(result) = loaded {
                // Route only after the load response: replayed history is dropped (spec §7).
                connection.register_route(&native, sink.clone());
                established = Some((native, false, Some(result), Prefs::default()));
            }
        }
    }
    let (native, fresh, advertised, applied) = match established {
        Some(found) => found,
        None => match connection
            .call(
                "session/new",
                json!({"cwd": cwd, "mcpServers": mcp_servers}),
            )
            .await
        {
            Ok(result) => {
                let Some(native) = result.get("sessionId").and_then(Value::as_str) else {
                    ctx.pool.release(agent, connection.generation()).await;
                    return Err(AgentErrorInfo {
                        code: None,
                        message: "session/new returned no sessionId".into(),
                        data: None,
                    });
                };
                let native = native.to_owned();
                connection.register_route(&native, sink);
                (native, true, Some(result), Prefs::default())
            }
            Err(error) => {
                ctx.pool.release(agent, connection.generation()).await;
                return Err(error.info());
            }
        },
    };
    // Defaults only for a fresh native session; a loaded one keeps its state except the session's own choices.
    let target = if fresh {
        plan.own.over_defaults(plan.defaults.as_ref())
    } else {
        plan.own
    };
    let applied = apply_prefs(&connection, &native, &target, applied).await;
    Ok(Ready {
        agent: agent.clone(),
        connection,
        native,
        fresh,
        advertised,
        applied,
    })
}

/// Sends the set calls that bring the native session from `applied` to `target`, awaiting each before the
/// prompt is sent. A refused call is logged and stays pending (retried before the next turn).
async fn apply_prefs(
    connection: &Arc<AcpConnection>,
    native: &str,
    target: &Prefs,
    mut applied: Prefs,
) -> Prefs {
    for (method, params) in target.calls_from(&applied, native) {
        match connection.call(method, params.clone()).await {
            Ok(_) => {
                if let Some(mode) = params.get("modeId").and_then(Value::as_str) {
                    applied.mode = Some(mode.to_owned());
                } else if let Some(model) = params.get("modelId").and_then(Value::as_str) {
                    applied.model = Some(model.to_owned());
                } else if let (Some(id), Some(value)) = (
                    params.get("configId").and_then(Value::as_str),
                    params.get("value").and_then(Value::as_str),
                ) {
                    applied.config.insert(id.to_owned(), value.to_owned());
                }
            }
            Err(error) => {
                // Newer agents (Claude Code ACP) list their models only as the `model` config option.
                if let Some(model) = params.get("modelId").and_then(Value::as_str) {
                    let retry = json!({"sessionId": native, "configId": "model", "value": model});
                    if connection
                        .call("session/set_config_option", retry)
                        .await
                        .is_ok()
                    {
                        applied.model = Some(model.to_owned());
                        continue;
                    }
                }
                tracing::warn!(method, code = error.code, message = %error.message, "agent refused option");
            }
        }
    }
    applied
}
