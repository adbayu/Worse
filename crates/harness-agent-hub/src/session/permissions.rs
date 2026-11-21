use super::{
    json, new_id, permission_option, scope_paths, store, tool_view, AcpConnection, Arc, EventKind,
    PendingPermission, PermissionDecision, PermissionOptionKind, PermissionRequestId,
    PermissionRequestView, SessionActor, Value,
};

impl SessionActor {
    #[allow(clippy::too_many_lines, clippy::single_match_else)]
    pub(super) async fn on_permission_request(
        &mut self,
        connection: Arc<AcpConnection>,
        rpc_id: Value,
        params: &Value,
    ) {
        let Some(running) = self.running.as_ref() else {
            return;
        };
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(permission_option)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let view = PermissionRequestView {
            request_id: PermissionRequestId::new(new_id("perm")),
            turn_id: running.turn_id.clone(),
            agent: running.agent.clone(),
            tool: tool_view(params.get("toolCall").unwrap_or(&Value::Null)),
            options,
        };
        if self.ctx.routers.is_router(&self.row.id) {
            let title = view.tool.title.to_lowercase();
            let allowed = [
                "harness-delegate",
                "harness_delegate",
                "list_agents",
                "route_trigger",
                "dismiss_trigger",
            ]
            .iter()
            .any(|name| title.contains(name));
            let wanted = if allowed {
                [
                    PermissionOptionKind::AllowAlways,
                    PermissionOptionKind::AllowOnce,
                ]
            } else {
                [
                    PermissionOptionKind::RejectOnce,
                    PermissionOptionKind::RejectAlways,
                ]
            };
            let option = wanted
                .iter()
                .find_map(|kind| view.options.iter().find(|o| o.kind == *kind))
                .cloned();
            let decision = match option {
                Some(option) => {
                    connection.respond(
                        &rpc_id,
                        json!({"outcome": {"outcome": "selected", "optionId": option.option_id}}),
                    );
                    PermissionDecision::Selected {
                        option_id: option.option_id,
                        kind: option.kind,
                    }
                }
                None => {
                    connection.respond(&rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
                    PermissionDecision::Cancelled
                }
            };
            self.record_permission(&view, decision).await;
            return;
        }
        if let Some(scope) = self.ctx.scope_of(&self.row.id) {
            let paths = scope_paths(&self.row.workspace_path, &view.tool);
            if let Some(reason) =
                crate::delegate::blocked(&scope, &view.tool, &paths, crate::util::now_ms())
            {
                let option = view
                    .options
                    .iter()
                    .find(|o| matches!(o.kind, PermissionOptionKind::RejectOnce))
                    .or_else(|| {
                        view.options
                            .iter()
                            .find(|o| matches!(o.kind, PermissionOptionKind::RejectAlways))
                    })
                    .cloned();
                let decision = match option {
                    Some(option) => {
                        connection.respond(
                            &rpc_id,
                            json!({"outcome": {"outcome": "selected", "optionId": option.option_id}}),
                        );
                        PermissionDecision::Selected {
                            option_id: option.option_id,
                            kind: option.kind,
                        }
                    }
                    None => {
                        connection.respond(&rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
                        PermissionDecision::Cancelled
                    }
                };
                if let Some(mut subagent) = self.ctx.subagents.get(&self.row.id) {
                    subagent.activity = format!(
                        "Scope denied: {} ({reason})",
                        if view.tool.title.is_empty() {
                            "a tool"
                        } else {
                            view.tool.title.as_str()
                        }
                    );
                    self.ctx.put_subagent(subagent).await;
                }
                self.record_permission(&view, decision).await;
                return;
            }
        }
        if self.ctx.machine.is_on(self.row.id.as_str()) {
            let allow = view
                .options
                .iter()
                .find(|o| matches!(o.kind, PermissionOptionKind::AllowAlways))
                .or_else(|| {
                    view.options
                        .iter()
                        .find(|o| matches!(o.kind, PermissionOptionKind::AllowOnce))
                })
                .cloned();
            if let Some(option) = allow {
                connection.respond(
                    &rpc_id,
                    json!({"outcome": {"outcome": "selected", "optionId": option.option_id}}),
                );
                let decision = PermissionDecision::Selected {
                    option_id: option.option_id,
                    kind: option.kind,
                };
                self.record_permission(&view, decision).await;
                return;
            }
        }
        self.pending.push(PendingPermission {
            view: view.clone(),
            rpc_id,
            connection,
        });
        self.emit(EventKind::PermissionRequested { request: view });
    }

    pub(super) fn cancel_pending_permissions(&mut self) {
        for pending in std::mem::take(&mut self.pending) {
            pending.connection.respond(
                &pending.rpc_id,
                json!({"outcome": {"outcome": "cancelled"}}),
            );
            if let Some(turn) = self.turn_mut(&pending.view.turn_id) {
                turn.permissions.push((
                    pending.view.tool.title.clone(),
                    PermissionDecision::Cancelled,
                ));
            }
            let payload = json!({"title": pending.view.tool.title, "decision": PermissionDecision::Cancelled});
            let store = self.ctx.store.clone();
            let turn_id = pending.view.turn_id.clone();
            tokio::spawn(async move {
                let _ = store
                    .run(move |c| store::append_turn_event(c, &turn_id, "permission", &payload))
                    .await;
            });
            self.emit(EventKind::PermissionResolved {
                turn_id: pending.view.turn_id,
                request_id: pending.view.request_id,
                decision: PermissionDecision::Cancelled,
            });
        }
    }

    pub(super) async fn record_permission(
        &mut self,
        view: &PermissionRequestView,
        decision: PermissionDecision,
    ) {
        if let Some(turn) = self.turn_mut(&view.turn_id) {
            turn.permissions
                .push((view.tool.title.clone(), decision.clone()));
        }
        self.append_event(
            &view.turn_id,
            "permission",
            json!({"title": view.tool.title, "agent": view.agent, "decision": decision}),
        )
        .await;
        self.emit(EventKind::PermissionResolved {
            turn_id: view.turn_id.clone(),
            request_id: view.request_id.clone(),
            decision,
        });
    }
}
